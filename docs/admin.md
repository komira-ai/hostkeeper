# hostkeeper admin

`hostkeeper admin` calls the daemon on the host it runs on, over the daemon's admin socket
(`hostkeeper.local.v1.Admin`, `crates/hostkeeper-proto/proto/hostkeeper/local/v1/admin.proto`).
It works with the coordinator down. It is the home of every call of the services in the packages
`hostkeeper.local.v<n>`; [`hostkeeper ctl`](ctl.md) calls the coordinator and has none of them.

```
hostkeeper admin status
hostkeeper admin abort <service>
hostkeeper admin clear-record <service>
```

| verb | RPC | what it does |
|---|---|---|
| `status` | `Status` | prints the host's enrollment, the heartbeat loop's link to the coordinator, the trusted root's age, and each installed service. Changes nothing. |
| `abort <service>` | `Abort` | stops the service's running step, and ends its order in the halt phase, as the coordinator's `abort` verb does. |
| `clear-record <service>` | `ClearRecord` | clears the service's step record when it cannot be read, so that orders run for the service again. A record that reads is refused (`record_reads`). |

No verb installs, updates or switches a version, and none lifts a halt: a halt is lifted only by
the coordinator's `clear_halt` order, which the admin socket does not carry.

## The socket

`--admin-socket <path>` names the socket, before or after the verb. Its default is the path the
daemon binds by default: `/run/hostkeeper/admin.sock` on Linux, `/var/run/hostkeeper/admin.sock`
on macOS (the constant `DEFAULT_ADMIN_SOCKET` of `crates/hostkeeper-daemon/src/cli.rs`, which
`hostkeeper daemon --admin-socket` defaults to as well). The socket is mode 0600 in a directory
of mode 0700, and the daemon serves only a peer whose uid is 0: run `hostkeeper admin` as root.
The path comes from the flag only, never from the environment.

## Output

A verb that succeeds prints its answer on standard output, one `key: value` per line, and exits
0. The lines and their order are stable, so a person at a console reads them and a script may
match them:

- an enum value is its name without its prefix, in lower case (`LIFECYCLE_RUNNING` is `running`);
  a value this binary does not know is `unknown_<number>`;
- an empty string field is `-`;
- a service's keys are under `service[<name>]`; a service name never holds `[`, `]`, `:` or a
  space.

`status` prints, in this order:

| key | value |
|---|---|
| `enrollment` | `unenrolled`, `pending`, `enrolled` or `unreadable` (see below); `unknown` before the enrollment loop's first step has ended, including while the loop cannot make its enroller (it logs `the enrollment loop cannot start yet` and tries again after 5 s, doubling up to 300 s) |
| `enrollment.key_sha256` | only when `pending`: the SHA-256 of the pending key's DER SubjectPublicKeyInfo, 64 lower-case hex digits |
| `enrollment.host_id` | only when `enrolled`: the host id the certificate names |
| `enrollment.not_after_unix` | only when `enrolled`: the last unix second the certificate is valid |
| `link.last_success_unix` | the unix second, by the host's wall clock, of the last heartbeat the coordinator answered; `never` when none was since the daemon started |
| `link.consecutive_failures` | the heartbeats in a row not sent or not answered since the last one answered, or since the daemon started |
| `link` | `unknown`, in place of the two lines above, before the heartbeat loop's first heartbeat has ended, and when the loop does not run |
| `trusted_root.age_secs` | the seconds since the Sigstore trusted root's file was last written on the host; `unknown` when the daemon reported no age (no file) |
| `trusted_root.reason` | why the trusted root needs attention: `trusted_root_stale`, `trusted_root_missing` or `trusted_root_unparseable`; `none` when it needs none |
| `services` | how many services follow |

A heartbeat the loop did not try to send, because the host has no certificate yet or its
installed services could not be read, changes neither `link` line.

Then, for each service in name order:

| key | value |
|---|---|
| `service[<name>].lifecycle` | `absent`, `installing`, `running` or `removing` |
| `service[<name>].record_problem` | `none`, or `unreadable` or `newer_schema` when its step record cannot be read as it is |
| `service[<name>].liveness` | `live`, `absent`, `stopped`, `transitioning`, `unobserved` or `unmonitored`; `unknown` when the service was not observed, and then the next three lines are left out |
| `service[<name>].version` | the version an active unit runs, when the daemon confirmed it; else `-` |
| `service[<name>].healthy` | `true` or `false` |
| `service[<name>].unhealthy` | why it is not healthy, or `none` |
| `service[<name>].current` | `none` when no order is under way; else the step record of the order under way, as the `.current.*` lines below |
| `service[<name>].last` | `none` when no order ended; else the step record of the last order that ended, as the `.last.*` lines below |

A step record is printed as these keys under its prefix (`service[<name>].current`,
`service[<name>].last`, or `record` for `abort`): `order_id`, `order_try`, `version`, `step`,
`outcome` (`succeeded`, `failed`, `deferred`, or `none` while it runs), `phase` and `class` (`none`
unless it failed with one), `reason`, `forced`, `started_unix` (when the daemon took the step lock
for the order) and `finished_unix` (when it ended; `none` while it runs). A `succeeded` order ended
with its version running and confirmed healthy, so the `finished_unix` of a service's first
install is when the host began serving it: the measurement of network install reads it
(`cargo xtask netinstall-report`, [network-install.md](operations/network-install.md#measure-the-first-machine)).

`abort <service>` prints `aborted: <service>` and then the order as its step record holds it
once it ended, under `record` (`record: none` when the daemon sent no record). `clear-record
<service>` prints `cleared: <service>` and `problem: unreadable`.

For example, a host waiting for its approval, whose heartbeats are not sent yet:

```
# hostkeeper admin status
enrollment: pending
enrollment.key_sha256: 4f2a9c0e8b7d6a5f4e3d2c1b0a99887766554433221100ffeeddccbbaa998877
link.last_success_unix: never
link.consecutive_failures: 0
trusted_root.age_secs: 86400
trusted_root.reason: none
services: 0
```

## Enrollment states

The daemon's enrollment loop reads the pki directory (`/etc/updater/pki`) after each of its
steps, with the same code a step uses to decide what to do next, and `status` shows what it read
last:

| state | the pki directory holds | what the loop does next |
|---|---|---|
| `unenrolled` | no certificate and no pending key | makes the pending key and posts the request, once the coordinator's CA matches the pinned `ca_sha256` |
| `pending` | no certificate, and a pending key | polls for the operator's approval of the request with this key |
| `enrolled` | a certificate of this host under `current`, valid or expired | renews it from the middle of its validity on; once it expired, enrolls again with the same key, `renewal_of` the host |
| `unreadable` | something it cannot read, or a `current` generation or pending key that is not a certificate and key of this host | nothing until it reads; the daemon's log says why |

The `key_sha256` of a pending host is the value `hostkeeper ctl enrollment list` shows for its
request and `hostkeeper ctl enrollment approve --key-sha256` compares
([enrollment-coordinator.md](enrollment-coordinator.md)): an operator at the host's console reads
it here and checks it against the coordinator's list before approving.

## Exit status

| code | when |
|---|---|
| 0 | the call succeeded |
| 1 | the socket could not be reached, the call broke off, or the daemon answered `NOT_FOUND` (`not_installed`), `INTERNAL` (`store_failed`) or another failure |
| 2 | a usage error, or the daemon refused the request: `INVALID_ARGUMENT` (`invalid_request`) or `FAILED_PRECONDITION` (`nothing_running`, `record_reads`, `busy`) |
| 3 | `UNAUTHENTICATED` or `PERMISSION_DENIED` |

These are the codes of `hostkeeper ctl` (`crates/hostkeeper-ctl/src/exit.rs`). A call that fails
prints nothing on standard output and one line on standard error: `hostkeeper admin: <message>
(<code>)` for an answer of the daemon, whose message is the refusal's token (for example
`hostkeeper admin: not_installed (NotFound)`), and `hostkeeper admin: cannot connect to
<socket>: <why>` for a socket that could not be reached.
