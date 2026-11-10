//! The tests of the rebase: the old container's configuration on the new image.

use serde_json::{Value, json};

use super::rebase::create_body;
use super::tests::{old_container, old_image};

const NEW: &str = "sha256:newimage";

#[test]
fn the_command_line_mounts_and_restart_policy_are_the_old_containers_and_the_image_is_replaced() {
    // Proves: the command line (Cmd) and every bind and volume are exactly the old container's,
    // the restart policy is kept, the image is the new id, and the container's own hostname is
    // not carried over.
    let old = old_container();
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert_eq!(body["Cmd"], old["Config"]["Cmd"]);
    assert_eq!(body["HostConfig"], old["HostConfig"]);
    assert_eq!(
        body["HostConfig"]["Binds"],
        json!([
            "/srv/hostkeeper-coordinator/etc:/etc/hostkeeper-coordinator:ro",
            "hostkeeper-coordinator_hostkeeper-coordinator:/var/lib/hostkeeper-coordinator"
        ])
    );
    assert_eq!(
        body["HostConfig"]["RestartPolicy"],
        json!({"Name": "unless-stopped", "MaximumRetryCount": 0})
    );
    assert_eq!(body["Image"], NEW);
    assert!(body.get("Hostname").is_none() && body.get("Domainname").is_none());
    assert!(body.get("NetworkingConfig").is_none());
}

#[test]
fn what_the_old_image_supplied_is_dropped_and_what_the_container_set_is_kept() {
    // Proves: an entrypoint, user and working directory equal to the old image's are left to the
    // new image; the environment and labels keep only what the container set; the command line
    // the container set differently from the image stays.
    let body = create_body(&old_container(), &old_image(), NEW).expect("a body");
    for supplied in ["Entrypoint", "User", "WorkingDir"] {
        assert!(body.get(supplied).is_none(), "{supplied}");
    }
    assert_eq!(body["Env"], json!(["TZ=UTC"]));
    assert_eq!(
        body["Labels"],
        json!({"com.docker.compose.service": "coordinator"})
    );
    assert_eq!(body["Cmd"].as_array().map(Vec::len), Some(5));

    let mut old = old_container();
    let mut image = old_image();
    old["Config"]["Cmd"] = json!(["serve"]);
    old["Config"]["Healthcheck"] = json!({"Test": ["CMD", "true"]});
    image["Config"]["Healthcheck"] = json!({"Test": ["CMD", "true"]});
    old["Config"]["StopSignal"] = json!("SIGTERM");
    old["Config"]["ExposedPorts"] = json!({"8444/tcp": {}});
    image["Config"]["ExposedPorts"] = json!({"8444/tcp": {}});
    old["Config"]["Volumes"] = json!({"/data": {}});
    old["Config"]["User"] = json!("1000");
    let body = create_body(&old, &image, NEW).expect("a body");
    for supplied in [
        "Cmd",
        "Healthcheck",
        "ExposedPorts",
        "Entrypoint",
        "WorkingDir",
    ] {
        assert!(body.get(supplied).is_none(), "{supplied}");
    }
    assert_eq!(body["StopSignal"], "SIGTERM");
    assert_eq!(body["Volumes"], json!({"/data": {}}));
    assert_eq!(body["User"], "1000");
}

#[test]
fn an_environment_and_labels_all_the_images_leave_no_key_and_an_image_without_config_drops_nothing()
{
    let mut old = old_container();
    old["Config"]["Env"] = json!(["PATH=/usr/local/bin"]);
    old["Config"]["Labels"] = json!({"org.opencontainers.image.version": "0.1.218"});
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert!(body.get("Env").is_none() && body.get("Labels").is_none());

    old["Config"]["Labels"] = json!({"org.opencontainers.image.version": "changed"});
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert_eq!(
        body["Labels"],
        json!({"org.opencontainers.image.version": "changed"})
    );

    let body = create_body(&old_container(), &json!({}), NEW).expect("a body");
    assert_eq!(body["Entrypoint"], old_container()["Config"]["Entrypoint"]);
    assert_eq!(body["Env"], old_container()["Config"]["Env"]);
    let mut bare = old_container();
    bare["Config"] = json!({"Image": "x"});
    let body = create_body(&bare, &old_image(), NEW).expect("a body");
    assert_eq!(body["Image"], NEW);
    assert!(body.get("Env").is_none());
}

#[test]
fn a_user_network_carries_its_aliases_and_fixed_addresses() {
    // Proves: for a network mode of a user network, the endpoints are the container's networks
    // with their aliases (without the container's own short id) and fixed addresses; the host's
    // and the other built-in modes have none.
    let mut old = old_container();
    old["HostConfig"]["NetworkMode"] = json!("coordinator_default");
    old["NetworkSettings"]["Networks"] = json!({
        "coordinator_default": {
            "Aliases": ["coordinator", "oldid0000000"],
            "IPAMConfig": {"IPv4Address": "198.51.100.5"},
            "IPAddress": "198.51.100.5",
            "NetworkID": "abc"
        },
        "plain": {"IPAMConfig": null}
    });
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert_eq!(
        body["NetworkingConfig"],
        json!({"EndpointsConfig": {
            "coordinator_default": {"Aliases": ["coordinator"], "IPAMConfig": {"IPv4Address": "198.51.100.5"}},
            "plain": {}
        }})
    );
    assert_eq!(body["HostConfig"]["NetworkMode"], "coordinator_default");

    for mode in ["host", "none", "default", "bridge", "", "container:abc"] {
        old["HostConfig"]["NetworkMode"] = json!(mode);
        let body = create_body(&old, &old_image(), NEW).expect("a body");
        assert!(body.get("NetworkingConfig").is_none(), "{mode}");
    }
    old["HostConfig"]["NetworkMode"] = json!("coordinator_default");
    old["NetworkSettings"]["Networks"] = json!({});
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert!(body.get("NetworkingConfig").is_none());
    old["HostConfig"]
        .as_object_mut()
        .expect("object")
        .remove("NetworkMode");
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert!(body.get("NetworkingConfig").is_none());
}

#[test]
fn a_volume_the_old_image_made_is_mounted_again_by_name() {
    // Proves: a volume the container mounts that no bind or mount of its HostConfig names (an
    // anonymous volume of the old image's VOLUME) is added as a bind of the same volume, so its
    // data is not left behind; volumes the HostConfig names are not added twice.
    let mut old = old_container();
    old["Mounts"].as_array_mut().expect("mounts").extend([
        json!({"Type": "volume", "Name": "anon1", "Destination": "/data", "RW": true}),
        json!({"Type": "volume", "Name": "anon2", "Destination": "/cache", "RW": false}),
        json!({"Type": "volume", "Name": "viamounts", "Destination": "/m", "RW": true}),
        json!({"Type": "volume", "Destination": "/nameless", "RW": true}),
        json!({"Type": "tmpfs", "Destination": "/tmp"}),
    ]);
    old["HostConfig"]["Mounts"] =
        json!([{"Type": "volume", "Source": "viamounts", "Target": "/m"}]);
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    let binds: Vec<&str> = body["HostConfig"]["Binds"]
        .as_array()
        .expect("binds")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        binds,
        [
            "/srv/hostkeeper-coordinator/etc:/etc/hostkeeper-coordinator:ro",
            "hostkeeper-coordinator_hostkeeper-coordinator:/var/lib/hostkeeper-coordinator",
            "anon1:/data",
            "anon2:/cache:ro",
        ]
    );
    assert_eq!(body["HostConfig"]["Mounts"], old["HostConfig"]["Mounts"]);

    let mut old = old_container();
    old["HostConfig"] = json!({"Binds": null});
    old["Mounts"] =
        json!([{"Type": "volume", "Name": "anon1", "Destination": "/data", "RW": true}]);
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert_eq!(body["HostConfig"], json!({"Binds": ["anon1:/data"]}));
    old.as_object_mut().expect("object").remove("HostConfig");
    let body = create_body(&old, &old_image(), NEW).expect("a body");
    assert_eq!(body["HostConfig"], json!({"Binds": ["anon1:/data"]}));
}

#[test]
fn an_inspect_without_a_config_is_refused() {
    assert_eq!(
        create_body(&json!({"Id": "x"}), &old_image(), NEW).expect_err("no config"),
        "the container's inspect has no Config"
    );
}
