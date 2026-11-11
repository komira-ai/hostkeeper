//! The create body of the new coordinator container: the old container, run on the new image.
//!
//! A pure function of the old container's inspect JSON, the old image's inspect JSON and the new
//! image's id. The new container takes the old container's `Config` with its `Image` replaced,
//! minus what the old image itself supplied, so the new image's own applies: a field of the
//! container that equals the image's own value was never set by the operator. What the operator
//! set (the command line, the environment, the labels, the mounts, the restart policy, the
//! published ports, the network) stays exactly as it was.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

/// The `Config` fields whose value, when it equals the old image's own, the new image supplies.
const IMAGE_SUPPLIED: [&str; 8] = [
    "Entrypoint",
    "Cmd",
    "User",
    "WorkingDir",
    "StopSignal",
    "Healthcheck",
    "ExposedPorts",
    "Volumes",
];

/// The network modes that have no endpoint of their own to carry over.
const NO_ENDPOINTS: [&str; 5] = ["", "host", "none", "default", "bridge"];

/// The body of `POST /containers/create` for the container `old` on the image `new_id`, where
/// `old_image` is the image `old` runs. The error says what the inspect JSON lacks.
pub(crate) fn create_body(old: &Value, old_image: &Value, new_id: &str) -> Result<Value, String> {
    let Some(config) = old["Config"].as_object() else {
        return Err("the container's inspect has no Config".to_owned());
    };
    let mut body = config.clone();
    body.remove("Hostname");
    body.remove("Domainname");
    let own = &old_image["Config"];
    for key in IMAGE_SUPPLIED {
        if body.get(key).unwrap_or(&Value::Null) == &own[key] {
            body.remove(key);
        }
    }
    drop_own_env(&mut body, &own["Env"]);
    drop_own_labels(&mut body, &own["Labels"]);
    body.insert("Image".to_owned(), json!(new_id));

    let mut host = old["HostConfig"].as_object().cloned().unwrap_or_default();
    let anonymous = anonymous_volumes(old);
    if !anonymous.is_empty() {
        let mut binds = host.get("Binds").and_then(Value::as_array).cloned();
        binds
            .get_or_insert_with(Vec::new)
            .extend(anonymous.into_iter().map(Value::String));
        host.insert("Binds".to_owned(), json!(binds));
    }
    let mode = host.get("NetworkMode").and_then(Value::as_str);
    let mode = mode.unwrap_or_default().to_owned();
    body.insert("HostConfig".to_owned(), Value::Object(host));
    if !NO_ENDPOINTS.contains(&mode.as_str()) && !mode.starts_with("container:") {
        let endpoints = endpoints(old);
        if !endpoints.is_empty() {
            body.insert(
                "NetworkingConfig".to_owned(),
                json!({ "EndpointsConfig": endpoints }),
            );
        }
    }
    Ok(Value::Object(body))
}

/// `Env` without the entries the old image supplies; the key goes when none are left.
fn drop_own_env(body: &mut Map<String, Value>, own: &Value) {
    let own: Vec<&Value> = own.as_array().into_iter().flatten().collect();
    let kept: Vec<Value> = body
        .get("Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| !own.contains(entry))
        .cloned()
        .collect();
    set_or_remove(body, "Env", &json!(kept));
}

/// `Labels` without the labels the old image supplies; the key goes when none are left.
fn drop_own_labels(body: &mut Map<String, Value>, own: &Value) {
    let kept: Map<String, Value> = body
        .get("Labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(label, value)| own.get(label.as_str()) != Some(*value))
        .map(|(label, value)| (label.clone(), value.clone()))
        .collect();
    set_or_remove(body, "Labels", &Value::Object(kept));
}

/// Sets `key` to `value` unless it is empty, in which case the key goes.
fn set_or_remove(body: &mut Map<String, Value>, key: &str, value: &Value) {
    let empty =
        value.as_array().is_some_and(Vec::is_empty) || value.as_object().is_some_and(Map::is_empty);
    if empty {
        body.remove(key);
    } else {
        body.insert(key.to_owned(), value.clone());
    }
}

/// The volumes the container mounts that its `HostConfig` does not name: anonymous volumes the
/// old image's `VOLUME` made. They are named as binds, so the new container mounts the same
/// volume and its data, not a new empty one.
fn anonymous_volumes(old: &Value) -> Vec<String> {
    let named: BTreeSet<&str> = old["HostConfig"]["Binds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_str()?.split(':').nth(1))
        .chain(
            old["HostConfig"]["Mounts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["Target"].as_str()),
        )
        .collect();
    old["Mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["Type"] == "volume")
        .filter_map(|m| {
            let (name, to) = (m["Name"].as_str()?, m["Destination"].as_str()?);
            let mode = if m["RW"] == false { ":ro" } else { "" };
            (!named.contains(to)).then(|| format!("{name}:{to}{mode}"))
        })
        .collect()
}

/// The endpoint of each network the container is on, with its aliases (but for the container's
/// own short id, which Docker adds to every user network) and its fixed addresses.
fn endpoints(old: &Value) -> Map<String, Value> {
    let id = old["Id"].as_str().unwrap_or_default();
    let mut out = Map::new();
    for (name, settings) in old["NetworkSettings"]["Networks"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let mut endpoint = Map::new();
        if let Some(aliases) = settings["Aliases"].as_array() {
            let kept: Vec<&Value> = aliases
                .iter()
                .filter(|a| a.as_str().is_some_and(|a| !id.starts_with(a)))
                .collect();
            endpoint.insert("Aliases".to_owned(), json!(kept));
        }
        if !settings["IPAMConfig"].is_null() {
            endpoint.insert("IPAMConfig".to_owned(), settings["IPAMConfig"].clone());
        }
        out.insert(name.clone(), Value::Object(endpoint));
    }
    out
}
