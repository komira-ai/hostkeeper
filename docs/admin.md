# A host's status, abort and a corrupt step record

The daemon serves nothing on the host: it binds no socket, and `hostkeeper admin` is gone (it is a
usage error, exit status 2). What the three calls of `hostkeeper admin` did, the coordinator does,
through [`hk`](cli.md):

| to | run | needs |
|---|---|---|
| read a host's status | `hk hosts get` | the host heard from since the coordinator started |
| stop a service's running step | `hk hosts abort` | any daemon that announces `abort-v1` |
| set aside a step record that does not read | `hk hosts clear-record` | a daemon that announces `clear-record-v1` |

Every `hk` call takes the endpoint, the CA file and the token of [cli.md](cli.md). A host the coordinator cannot reach is the last section.

No call installs, updates or switches a version, and none lifts a halt: a halt is lifted only by the
coordinator's `clear_halt` order.

To switch a service back with the coordinator out of reach, root runs `hostkeeper activate` on the
host itself: [Roll a service back on the host](#roll-a-service-back-on-the-host).

## Status

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id>
```

The answer is the coordinator's view of the host: whether it is live, when its last heartbeat
arrived, that heartbeat as the host sent it (its agent version, the trusted root's age and reason in
`trusted_root_age_secs` and `trusted_root_reason`, see
[The trusted root on a host](deploy/verify-a-release.md#the-trusted-root-on-a-host), and each
service's lifecycle, health and step records), the host's Full Disk Access state in
`full_disk_access` (a Mac's grant: `GRANTED`, `NOT_GRANTED`, `UNDETERMINED`; `NOT_APPLICABLE` for a
Linux host, `UNSPECIFIED` when the host has not said, see [Full Disk Access](macos.md#full-disk-access)), and how each service the coordinator manages sees it
(`services`; its `health` is what the host last reported while the host is live, and `HEALTH_SILENT` once the host is not live, that is
when it has sent no heartbeat for 45 seconds or more, whatever it last reported; the heartbeat above keeps what the host sent),
and `running`: each service the host's daemon said it runs (not one it reported absent) with its
version and the installer it runs it with (`hostkeeper-archive` for a daemon that announces none),
and `running_unix`, when the coordinator took it from a heartbeat. A host the coordinator has not
heard from since it started is refused `unknown_host` by `hosts get`; `hosts list` lists it all the
same when it is enrolled and not denied, not live, with the daemon's version and `running` as its
enrollment kept them from its last change (the installer of a service is empty when the daemon
announces `target-v1` and the coordinator has read its installers over neither gRPC nor an earlier
record).

The enrollment of a host that has no certificate yet is read at the coordinator, not on the host:
`hk enrollment list` shows the request with its `key_sha256`, which an operator at the host's
console checks against the SHA-256 of the host's pending key
([enrollment.md](operations/enrollment.md#check-the-key-hash)).

## Abort

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts abort --node-id <id> --service <service> --reason <why>
```

The coordinator orders the host `abort` for the service in its next heartbeat answer; the order
takes no farm slot. The daemon stops the service's running step and ends its update in the halt
phase with the reason `aborted_by_operator`: the halt is that order's verdict, which comes back in a
heartbeat and `hk hosts get` shows. The abort's own answer is only in the daemon's log, where a
service with no step running is `nothing_running`, and nothing changes. A daemon that
does not announce `abort-v1` is refused `daemon_too_old` and nothing is ordered. An aborted service
takes no other order until its halt is lifted.

## A step record that does not read

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts clear-record --node-id <id> --service <service>
```

The coordinator orders the host `clear_record` for the service, once, in its next heartbeat answer.
A host whose daemon does not announce `clear-record-v1` is refused `daemon_too_old`, an unknown
host `unknown_host`, and nothing is ordered. The daemon runs the order under the service's lock,
without waiting, and answers with one verdict (`hk hosts get` shows it):

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

## Roll a service back on the host

With the coordinator or the network down, root switches a service to a version the host keeps,
fetching nothing, and can pin it there so that the coordinator's orders wait until root says
otherwise. A host keeps `current`, `previous`, the pinned version and the newest
`--keep-versions` others of each service (see the README's "The versions a host keeps").

1. Stop the daemon: `systemctl stop hostkeeper` on Linux, `rc-service hostkeeper stop` under
   OpenRC, `launchctl bootout system/dev.hostkeeper.hostkeeper` on a Mac. `activate` takes the
   daemon's lock and refuses to run beside it, naming its pid (exit status 4).
2. Switch, and pin:

   ```
   hostkeeper activate --service <service> --version <version> --pin
   ```

   It prints `<service>: <version> is current, <left> previous, pinned at <version>`. A version the
   host does not keep is refused `not_kept`, with the versions it keeps; one whose files changed
   since it was staged is refused `release_unreadable`. Nothing of the version left is run: its
   units are stopped by name, and the version switched to is installed from its own directory.
   When its units do not start, `activate` says `failed: start_failed` and leaves the host on that
   version: run `activate` again to another kept version. Pass the daemon's `--config`,
   `--state-dir` and `--packages-dir` when its unit gives others than the defaults.
3. Start the daemon again (`systemctl start hostkeeper`, `rc-service hostkeeper start`, or on a Mac
   `launchctl bootstrap system /Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`). If
   `activate` was stopped half-way, by a crash or a power cut, the daemon's start finishes the switch.

While the pin stands, the daemon defers every order of the coordinator that would change the
service (an update, a rollback, an install, an uninstall or a force to another version), and the
coordinator shows the deferral's reason, `pinned`. `hk hosts get` shows the version the host runs.

- `hostkeeper unpin --service <service>` removes the pin; the coordinator's next order runs, after
  the backoff of its deferrals (at most an hour). The daemon may run meanwhile.
- `hostkeeper pin --service <service> --version <version>` pins without switching, for a version
  the host keeps.
- A coordinator force to the pinned version (`hk releases force`) removes the pin and hands the
  service back to the coordinator.

The pin is the file `/var/lib/updater/pins/<service>` (beside the daemon's `--state-dir`): the
version and one newline, mode 0600, root's. A file there that is a link, another account's or
writable by group or others is not a pin: the daemon follows the coordinator and logs the alert
`not_a_pin`. A file of root's that names no version holds the service all the same
(`pin_unreadable`) until it is fixed or removed.

## Hosts that have not updated yet

A daemon from before this change still binds its admin socket (`/run/hostkeeper/admin.sock`,
`/var/run/hostkeeper/admin.sock` on a Mac) and still serves `hostkeeper.local.v1.Admin` to root. An
older `hostkeeper admin status|abort|clear-record` keeps working against it until the host updates.
The current daemon binds nothing and has no `--admin-socket` flag: the flag is a usage error
(exit 2). See [the design](design/two-programs.md#4-the-staged-path-for-the-daemon). The file
`crates/hostkeeper-proto/proto/hostkeeper/local/v1/admin.proto` stays, frozen, because the older
binary speaks it; no program serves it and no code is generated from it.
