//! The times of one machine's install, read from the text of each input file. Pure: each
//! function is of its text and its flags only.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Error, Mac, Mode};

/// What `hostkeeper-pxe serve` writes before each line of its log on standard error.
pub(crate) const PXE_PREFIX: &str = "hostkeeper-pxe serve: ";

/// The six events of an install, each in unix seconds; `None` for one the files do not show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Events {
    /// `hostkeeper-pxe`'s first DHCPDISCOVER (proxy mode) or DHCPREQUEST (router mode) from the
    /// MAC.
    pub(crate) power_on: Option<u64>,
    /// `hostkeeper-pxe`'s seed fetch of the MAC as the record's host.
    pub(crate) install_started: Option<u64>,
    /// The coordinator's `first_seen_unix` of the request matched to the record.
    pub(crate) pending: Option<u64>,
    /// The coordinator's `enrolled_unix` of the host.
    pub(crate) approved: Option<u64>,
    /// The coordinator's `first_heartbeat_unix` of the host.
    pub(crate) first_heartbeat: Option<u64>,
    /// The first of the services that the host's daemon installed and confirmed healthy.
    pub(crate) serving: Option<Serving>,
}

/// When a service's install ended with its version running and confirmed healthy, by the host's
/// clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serving {
    pub(crate) unix: u64,
    pub(crate) service: String,
}

/// `text` as unix seconds: one or more ASCII digits, nothing else.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A line of `serve`'s log as its unix seconds and its text, with or without [`PXE_PREFIX`];
/// `None` for a line that is not one.
fn stamped(line: &str) -> Option<(u64, &str)> {
    let line = line.strip_prefix(PXE_PREFIX).unwrap_or(line);
    let (secs, text) = line.split_once(' ')?;
    Some((digits(secs)?, text))
}

/// Power-on and the start of the install of `mac` as the record's host `host_id`, from the log
/// of `serve` in `mode`: the earliest line of each. Every other line is passed over.
pub(crate) fn pxe_log(
    text: &str,
    mac: &Mac,
    mode: Mode,
    host_id: &str,
) -> (Option<u64>, Option<u64>) {
    let asked = format!("dhcp: {} from {mac}, ", mode.power_on());
    let seed = format!("seed seed/{mac}/");
    let fetched = format!(": fetched by {mac}, host {host_id}: its install started");
    let earliest = |is: &dyn Fn(&str) -> bool| {
        text.lines()
            .filter_map(stamped)
            .filter(|(_, text)| is(text))
            .map(|(secs, _)| secs)
            .min()
    };
    (
        earliest(&|text| text.starts_with(&asked)),
        earliest(&|text| text.starts_with(&seed) && text.contains(&fetched)),
    )
}

/// `text` as a JSON object, what `hostkeeper ctl --output json` prints.
fn object(flag: &str, text: &str) -> Result<Value, Error> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| Error::Refused(format!("{flag}: not JSON: {e}")))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(Error::Refused(format!(
            "{flag}: not a JSON object, as `hostkeeper ctl --output json` prints"
        )))
    }
}

/// The unix seconds of `key` in `object`, as ctl prints a `uint64` (a string of digits); `None`
/// when it is absent or 0, which ctl leaves out.
fn unix(flag: &str, object: &Value, key: &str) -> Result<Option<u64>, Error> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => match value.as_str().and_then(digits) {
            Some(0) => Ok(None),
            Some(secs) => Ok(Some(secs)),
            None => Err(Error::Refused(format!(
                "{flag}: `{key}` is {value}, not unix seconds as a string of digits"
            ))),
        },
    }
}

/// Pending: the earliest `firstSeenUnix` of the requests matched to the record of `host_id`,
/// from what `enrollment list` printed. A list with no request of it (ctl prints an empty list
/// as `{}`) is `None`.
pub(crate) fn pending(text: &str, host_id: &str) -> Result<Option<u64>, Error> {
    let list = object("--pending", text)?;
    let requests = match list.get("requests") {
        None => return Ok(None),
        Some(Value::Array(requests)) => requests,
        Some(_) => {
            return Err(Error::Refused(
                "--pending: `requests` is not a list, as `enrollment list` prints".to_owned(),
            ));
        }
    };
    let mut first = None;
    for request in requests {
        if request.get("recordHostId").and_then(Value::as_str) != Some(host_id) {
            continue;
        }
        if let Some(seen) = unix("--pending", request, "firstSeenUnix")? {
            first = Some(first.map_or(seen, |earlier: u64| earlier.min(seen)));
        }
    }
    Ok(first)
}

/// Approved and the first heartbeat of `host_id`, from what `hosts get` printed of it.
pub(crate) fn host(text: &str, host_id: &str) -> Result<(Option<u64>, Option<u64>), Error> {
    let got = object("--host", text)?;
    let Some(host) = got.get("host").filter(|host| host.is_object()) else {
        return Err(Error::Refused(
            "--host: no `host`, as `hosts get` prints".to_owned(),
        ));
    };
    match host.get("nodeId").and_then(Value::as_str) {
        Some(node) if node == host_id => {}
        node => {
            return Err(Error::Refused(format!(
                "--host: the host is {}, not {host_id}",
                node.unwrap_or("not named")
            )));
        }
    }
    Ok((
        unix("--host", host, "enrolledUnix")?,
        unix("--host", host, "firstHeartbeatUnix")?,
    ))
}

/// Serving: the earliest of `services` whose last order ended `succeeded`, and when, from what
/// `hostkeeper admin status` printed. A service it does not list, or whose last order did not
/// succeed, is not serving.
pub(crate) fn admin_status(text: &str, services: &[String]) -> Result<Option<Serving>, Error> {
    let keys: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once(": ")).collect();
    if !keys.contains_key("services") {
        return Err(Error::Refused(
            "--admin-status: no `services` line, as `hostkeeper admin status` prints".to_owned(),
        ));
    }
    let mut first: Option<Serving> = None;
    for service in services {
        let last = format!("service[{service}].last");
        if keys.get(format!("{last}.outcome").as_str()) != Some(&"succeeded") {
            continue;
        }
        let key = format!("{last}.finished_unix");
        let unix = match keys.get(key.as_str()) {
            None => {
                return Err(Error::Refused(format!(
                    "--admin-status: no `{key}`: the host's hostkeeper predates the step \
                     record's times"
                )));
            }
            Some(value) => digits(value).ok_or_else(|| {
                Error::Refused(format!(
                    "--admin-status: `{key}` is `{value}`, not unix seconds"
                ))
            })?,
        };
        if first.as_ref().is_none_or(|f| unix < f.unix) {
            first = Some(Serving {
                unix,
                service: service.clone(),
            });
        }
    }
    Ok(first)
}
