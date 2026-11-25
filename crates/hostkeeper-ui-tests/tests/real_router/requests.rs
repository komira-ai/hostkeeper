//! The requests `real_router.rs` sends to the real coordinator, one for each method of
//! `ui/calls.json`, kept apart so that the test file stays short.

/// One request for each method of `ui/calls.json`, as proto3 JSON, in the shape the page that
/// calls the method sends it (`ui/pages/*.js`), with values the coordinator takes: a service, a
/// role, a host id, a reason. A host or request id no heartbeat or
/// enrollment named is answered `NOT_FOUND` or the like, which passes.
pub const REQUESTS: &[(&str, &str)] = &[
    ("hostkeeper.operator.v1.Alerts/List", "{}"),
    (
        "hostkeeper.operator.v1.ApiTokens/Create",
        r#"{"name": "build-script"}"#,
    ),
    ("hostkeeper.operator.v1.ApiTokens/List", "{}"),
    (
        "hostkeeper.operator.v1.ApiTokens/Revoke",
        r#"{"name": "build-script"}"#,
    ),
    (
        "hostkeeper.operator.v1.Audit/List",
        r#"{"sinceSeq": "0", "limit": 100, "service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/AddRecord",
        r#"{"hostId": "storage-1", "role": "storage", "dataMacs": ["02:00:00:00:00:01"],
            "bmcMac": "", "serial": "", "notes": "racked", "approveForSecs": "7200"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/Approve",
        r#"{"requestId": "1111111111111111111111111111111111111111111111111111111111111111",
            "keySha256": "2222222222222222222222222222222222222222222222222222222222222222",
            "hostId": "storage-1", "hostGroup": "storage", "replace": false}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/List",
        r#"{"limit": 100, "pageToken": ""}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/ListRecords",
        r#"{"limit": 100, "pageToken": ""}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/Reject",
        r#"{"requestId": "1111111111111111111111111111111111111111111111111111111111111111",
            "reason": "not one of ours"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/RemoveRecord",
        r#"{"hostId": "storage-1", "reason": "decommissioned"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/UpdateRecord",
        r#"{"hostId": "storage-1", "dataMacs": ["0c:c4:7a:00:00:02"], "bmcMac": "",
            "notes": "rack 4\nslot 2"}"#,
    ),
    (
        "hostkeeper.operator.v1.HostGroups/Get",
        r#"{"hostGroup": "storage", "limit": 50}"#,
    ),
    ("hostkeeper.operator.v1.HostGroups/List", "{}"),
    (
        "hostkeeper.operator.v1.HostGroups/Put",
        r#"{"hostGroup": {"name": "web", "packages": ["hostkeeper", "tierstore"]},
            "reason": "set in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.HostGroups/Remove",
        r#"{"hostGroup": "storage", "reason": "removed in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.HostGroups/Move",
        r#"{"nodeId": "storage-1", "hostGroup": "storage", "reason": "rebuilt"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Abort",
        r#"{"nodeId": "storage-1", "service": "kbf", "reason": "stuck"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Exclude",
        r#"{"service": "kbf", "nodeId": "storage-1", "reason": "disk swap", "abandon": false}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Get",
        r#"{"nodeId": "storage-1"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Include",
        r#"{"service": "kbf", "nodeId": "storage-1"}"#,
    ),
    ("hostkeeper.operator.v1.Hosts/List", r#"{}"#),
    (
        "hostkeeper.operator.v1.Hosts/Retire",
        r#"{"nodeId": "storage-1", "reason": "board failed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Retry",
        r#"{"nodeId": "storage-1", "service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/SetLabels",
        r#"{"nodeId": "storage-1", "labels": {"rack": "r1"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Ssh/HostKeys",
        r#"{"hostId": "storage-1"}"#,
    ),
    (
        "hostkeeper.operator.v1.Ssh/PinHostKeys",
        r#"{"hostId": "storage-1",
            "fingerprints": ["SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"]}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/AddRow",
        r#"{"reason": "add row"}"#,
    ),
    (
        "hostkeeper.operator.v1.Console/Close",
        r#"{"hostId":"storage-1"}"#,
    ),
    ("hostkeeper.operator.v1.Console/List", "{}"),
    (
        "hostkeeper.operator.v1.Console/Open",
        r#"{"hostId":"storage-1","reason":"checking"}"#,
    ),
    (
        "hostkeeper.operator.v1.Console/Power",
        r#"{"hostId":"storage-1","action":"POWER_ACTION_STATUS","reason":"checking"}"#,
    ),
    (
        "hostkeeper.operator.v1.Console/SetBootDevice",
        r#"{"hostId":"storage-1","device":"BOOT_DEVICE_DISK","persistent":true,"efi":true,"reason":"checking"}"#,
    ),
    ("hostkeeper.operator.v1.Layout/Get", "{}"),
    (
        "hostkeeper.operator.v1.Layout/Place",
        r#"{"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "storage-1"},
            "sled": "sled-1", "slot": 1, "reason": "lab move"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/PutName",
        r#"{"name": "Lab 1", "reason": "rename"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/PutRack",
        r#"{"row": "row-1", "heightU": 42, "reason": "add rack"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/PutSled",
        r#"{"rack": "r", "u": 1, "heightU": 1, "reason": "x"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/PutDevice",
        r#"{"name": "bmc-switch", "role": "NETWORK_ROLE_BMC", "consoleHost": "storage-1",
            "consoleDevice": "/dev/ttyUSB0", "reason": "added"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/RemoveDevice",
        r#"{"id": "net.1", "reason": "removed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/RemoveRack",
        r#"{"id": "rack-1", "reason": "remove"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/RemoveSled",
        r#"{"id": "s", "reason": "x"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/RemoveRow",
        r#"{"id": "row-1", "reason": "remove"}"#,
    ),
    (
        "hostkeeper.operator.v1.Layout/Unplace",
        r#"{"subject": {"kind": "LAYOUT_SUBJECT_KIND_INVENTORY", "id": "storage-1"},
            "reason": "retired"}"#,
    ),
    ("hostkeeper.operator.v1.Xcode/List", "{}"),
    (
        "hostkeeper.operator.v1.Xcode/Remove",
        r#"{"sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            "reason": "superseded"}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/Cancel",
        r#"{"runId": "no-such-run", "reason": "not needed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/Get",
        r#"{"runId": "no-such-run"}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/List",
        r#"{"limit": 10}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/PutBack",
        r#"{"runId": "no-such-run", "hostId": "storage-1", "reason": "put back"}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/RetryHost",
        r#"{"runId": "no-such-run", "hostId": "storage-1", "reason": "retry"}"#,
    ),
    (
        "hostkeeper.operator.v1.Maintenance/Schedule",
        r#"{"hostGroup": "storage", "reason": "monthly update", "confirm": true}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/ClearHalt",
        r#"{"service": "kbf", "reason": "fixed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Converge",
        r#"{"service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Force",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "hotfix", "includeHeld": false,
            "fleet": {"confirmService": "kbf"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/GetRelease",
        r#"{"service": "kbf", "version": "1.2.3"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Halt",
        r#"{"service": "kbf", "reason": "bad canary"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/MarkBad",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "crashes"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/SetDesired",
        r#"{"service": "kbf", "desired": {"version": "1.2.3"}}"#,
    ),
    ("hostkeeper.operator.v1.Releases/Status", r#"{}"#),
    (
        "hostkeeper.operator.v1.Releases/UnmarkBad",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "was the disk"}"#,
    ),
    // The two reads of a repository name a credential file that is not there: the coordinator
    // answers `credential_unusable` before it opens a connection, so no request leaves the test.
    (
        "hostkeeper.operator.v1.Repositories/Add",
        r#"{"name": "spare", "baseUri": "https://spare.example.com", "access": "hosts",
            "credentialFile": "/nonexistent/spare.credential",
            "descriptorSha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "fingerprints": ["0123456789ABCDEF0123456789ABCDEF01234567"],
            "reason": "set in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Repositories/Discover",
        r#"{"baseUri": "https://spare.example.com", "access": "hosts",
            "credentialFile": "/nonexistent/spare.credential"}"#,
    ),
    ("hostkeeper.operator.v1.Repositories/List", "{}"),
    (
        "hostkeeper.operator.v1.Repositories/Remove",
        r#"{"name": "spare", "reason": "removed in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Advance",
        r#"{"service": "kbf", "reason": "canary looks fine"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Pause",
        r#"{"service": "kbf", "reason": "freeze for audit"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Resume",
        r#"{"service": "kbf", "reason": "audit done"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Assign",
        r#"{"service": "tierstore", "role": "storage"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Hold",
        r#"{"scope": {"host": "storage-1"}, "service": "kbf", "reason": "disk swap"}"#,
    ),
    ("hostkeeper.operator.v1.Services/List", r#"{}"#),
    (
        "hostkeeper.operator.v1.Services/Put",
        r#"{"service": "js-agent", "repository": "komira", "reason": "set in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Remove",
        r#"{"service": "js-agent", "reason": "removed in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/ListTrustedPublishers",
        r#"{"service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/PutTrustedPublisher",
        r#"{"service": "kbf", "repository": "komira-ai/kbf", "workflow": ".github/workflows/release.yml", "ref": "main", "reason": "set in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/RemoveTrustedPublisher",
        r#"{"service": "kbf", "repository": "komira-ai/kbf", "workflow": ".github/workflows/release.yml", "ref": "main", "reason": "removed in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/SetRepository",
        r#"{"service": "kbf", "repository": "komira", "reason": "set in the console"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/SetMinVersion",
        r#"{"service": "kbf", "minVersion": {"role": "storage", "version": "1.0.0"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Unassign",
        r#"{"service": "tierstore", "role": "storage", "uninstall": false}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Unhold",
        r#"{"scope": {"host": "storage-1"}, "service": "kbf", "reason": "swapped"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Create",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Delete",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Disable",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Enable",
        r#"{"name": "alice"}"#,
    ),
    ("hostkeeper.operator.v1.Users/List", "{}"),
    (
        "hostkeeper.operator.v1.Users/ResetPassword",
        r#"{"name": "alice"}"#,
    ),
];
