# A host's status, abort and a corrupt step record

The daemon serves nothing on the host: it binds no socket, and `hostkeeper admin` is gone (it is a
usage error, exit status 2). What the three calls of `hostkeeper admin` did, the coordinator does,
through [`hkctl`](cli.md):

| to | run | needs |
|---|---|---|
| read a host's status | `hkctl hosts get` | the host heard from since the coordinator started |
| stop a service's running step | `hkctl hosts abort` | any daemon that announces `abort-v1` |
| set aside a step record that does not read | `hkctl hosts clear-record` | a daemon that announces `clear-record-v1` |

Every `hkctl` call takes the endpoint, the CA file and the token of [cli.md](cli.md). A host the coordinator cannot reach is the last section.

No call installs, updates or switches a version, and none lifts a halt: a halt is lifted only by the
coordinator's `clear_halt` order.

## Status

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id>
```

The answer is the coordinator's view of the host: whether it is live, when its last heartbeat
arrived, that heartbeat as the host sent it (its agent version, the trusted root's age and reason in
`trusted_root_age_secs` and `trusted_root_reason`, see
[The trusted root on a host](deploy/verify-a-release.md#the-trusted-root-on-a-host), and each
service's lifecycle, health and step records), and how each service the coordinator manages sees it,
and `running`: each service the host's daemon said it runs (not one it reported absent) with its
version and the installer it runs it with (`hostkeeper-archive` for a daemon that announces none),
and `running_unix`, when the coordinator took it from a heartbeat. A host the coordinator has not
heard from since it started is refused `unknown_host` by `hosts get`; `hosts list` lists it all the
same when it is enrolled and not denied, not live, with the daemon's version and `running` as its
enrollment kept them from its last change (the installer of a service is empty when the daemon
announces `target-v1` and the coordinator has read its installers over neither gRPC nor an earlier
record).

The enrollment of a host that has no certificate yet is read at the coordinator, not on the host:
`hkctl enrollment list` shows the request with its `key_sha256`, which an operator at the host's
console checks against the SHA-256 of the host's pending key
([enrollment.md](operations/enrollment.md#check-the-key-hash)).

## Abort

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts abort --node-id <id> --service <service> --reason <why>
```

The coordinator orders the host `abort` for the service in its next heartbeat answer; the order
takes no farm slot. The daemon stops the service's running step and ends its update in the halt
phase with the reason `aborted_by_operator`: the halt is that order's verdict, which comes back in a
heartbeat and `hkctl hosts get` shows. The abort's own answer is only in the daemon's log, where a
service with no step running is `nothing_running`, and nothing changes. A daemon that
does not announce `abort-v1` is refused `daemon_too_old` and nothing is ordered. An aborted service
takes no other order until its halt is lifted.

## A step record that does not read

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts clear-record --node-id <id> --service <service>
```

The coordinator orders the host `clear_record` for the service, once, in its next heartbeat answer.
A host whose daemon does not announce `clear-record-v1` is refused `daemon_too_old`, an unknown
host `unknown_host`, and nothing is ordered. The daemon runs the order under the service's lock,
without waiting, and answers with one verdict (`hkctl hosts get` shows it):

| verdict | meaning |
|---|---|
| `record_cleared` | the record did not read; it was renamed `step-record.json.corrupt` in the service's state directory, for the operator to read, and nothing else changed |
| `record_reads` | the record reads (or is absent, or was written by a newer hostkeeper): nothing was changed |
| `busy` | the service's lock is held by an order under way: nothing was changed; try again once it ends |
| `store_failed` | the rename failed: nothing was changed; the daemon's log says why |

The call is audited `record_cleared` at the coordinator.

## A host the coordinator cannot reach

When the link is down, root works on the host itself.

- **Status.** The daemon logs to the journal: `journalctl -u hostkeeper` on Linux. On a Mac its
  standard error is `/var/log/hostkeeper/daemon.err.log`. `systemctl status hostkeeper` shows
  whether it runs.
- **Stop a running step.** Stop the daemon: `systemctl stop hostkeeper` on Linux,
  `launchctl bootout system/dev.hostkeeper.hostkeeper` on a Mac. This ends the running step. A unit
  the step started may outlive it: find it (`systemctl list-units 'updater-<service>-*'`) and stop
  it by name.
- **A step record that does not read.** If the record must go before the link is back, stop the
  daemon as above, then rename the record in the service's state directory (the daemon's
  `--state-dir`, `/var/lib/updater/state` by default, holds one directory per service):

  ```
  mv /var/lib/updater/state/<service>/step-record.json \
     /var/lib/updater/state/<service>/step-record.json.corrupt
  ```

  That rename is the whole of what `clear-record` does. Do it only for a record that does not
  read: a record that reads is an update in progress. Then start the daemon again
  (`systemctl start hostkeeper`; on a Mac `launchctl bootstrap system
  /Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`).

## Hosts that have not updated yet

A daemon from before this change still binds its admin socket (`/run/hostkeeper/admin.sock`,
`/var/run/hostkeeper/admin.sock` on a Mac) and still serves `hostkeeper.local.v1.Admin` to root. An
older `hostkeeper admin status|abort|clear-record` keeps working against it until the host updates.
The newer daemon accepts the flag `--admin-socket <path>` that its unit still passes and ignores it:
it binds nothing. The flag goes in a later stage of
[the design](design/two-programs.md#4-the-staged-path-for-the-daemon). The file
`crates/hostkeeper-proto/proto/hostkeeper/local/v1/admin.proto` stays, frozen, because the older
binary speaks it; no program serves it and no code is generated from it.
