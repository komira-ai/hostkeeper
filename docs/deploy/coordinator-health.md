# Probe the coordinator's health

The coordinator's container image is distroless: no shell and no client, so nothing inside the
container can run a health check, and the image has no `HEALTHCHECK`. Health is a route instead:
`serve --health-listen <address:port>` binds one more listener, and a probe asks it
`GET /healthz` (is the process alive: what the orchestrator or the machine's monitoring checks) or
`GET /readyz` (does it take traffic: what the edge checks). The design is
[two-programs.md](../design/two-programs.md), section 2.7; the routing, the health checks and the
deploy check are [coordinator-multi-instance.md](../design/coordinator-multi-instance.md), section 11.
The edge's own health check and the start-first update are not built yet (row M10 of that
document): the deploy stays one instance, stop-first.

## The flag

```sh
hostkeeper-coordinator serve ... --health-listen 127.0.0.1:9100
```

- **Which address.** A loopback address (`127.0.0.0/8`, `::1`, or an IPv4 loopback written as IPv6,
  `[::ffff:127.0.0.1]`), or the address of one interface of the machine, for example the task's
  address on the overlay network the edge checks at. A wildcard (`0.0.0.0`, `[::]`,
  `[::ffff:0.0.0.0]`) stops the start with
  `--health-listen: <address> is a wildcard address; the health listener answers with no token and
  no TLS, so name a loopback address or the address of one interface, or give
  --health-listen-wildcard in a network namespace whose interfaces are loopback and one private
  network`.
- **`--health-listen-wildcard`** takes a wildcard `--health-listen`. It is off unless given, takes
  no value, and needs `--health-listen` (a usage error, exit 2, otherwise). Use it for a coordinator
  in a network namespace of its own whose interfaces are loopback and one private network, such as a
  Swarm task on one overlay network where the edge checks `/readyz`: the task does not know the
  overlay address before it starts. The listener then answers on every interface of the namespace.
  On the host's network, do not give it: the listener, which has no token and no TLS, would be on
  every network the host is on.
- **Plain HTTP, no token.** The listener speaks HTTP/1.1 with no TLS and reads no token: a probe
  needs neither a secret nor the farm's CA. Each connection answers one request and is closed.
- **Without the flag** there is no health listener.
- **The start line** names it last, as `health=<address:port>`, after `heartbeat=`, `control=`
  and, with `--lan-listen`, `lan=`.

On the host's network (as `--private-listen` needs without its wildcard flag), the loopback
address inside the container is the machine's own, and a probe on the machine reaches it.

## What it answers

Two paths, each for one reader.

| Request | Status | Body |
|---|---|---|
| `GET /healthz`, every listener the flags name bound | `200` | `ok` |
| `GET /healthz`, a listener not yet bound | `503` | `starting` |
| `GET /readyz`, every listener bound, the store takes writes, not draining | `200` | `ok` |
| `GET /readyz`, from SIGTERM or SIGINT until the process exits | `503` | `draining` |
| `GET /readyz`, a listener not yet bound | `503` | `starting` |
| `GET /readyz`, the database's link lost | `503` | `store_unreachable` |
| `HEAD` of either | as `GET` | none |
| any other path, or any other method (`POST` among them) | `404` | `not_found` |

A query on a path is ignored. When more than one reason holds, `/readyz` names one in this order:
`draining`, `starting`, `store_unreachable`.

`/healthz` is liveness. It is `starting` until every listener is bound, then `ok` for good: it stays
`ok` while the database's link is lost and while the coordinator drains, so an orchestrator that
checks it never kills a coordinator for a database that does not answer or for a stop in progress.
`/readyz` is readiness: it is the one an edge checks, and it says whether to send this coordinator
traffic.

`200 ok` on `/readyz` means every listener the flags name is bound and serving and the store takes
writes. The file store's link is never lost, so a file-store coordinator is `ok` once it serves; with
the Postgres store (`--database-dsn-file`) it is `ok` while the session with the database holds
([coordinator-storage.md](../design/coordinator-storage.md), section 5). The body is the token and
nothing else, as `text/plain; charset=utf-8` with `cache-control: no-store`; a `HEAD` gets the
status and the `content-length` and no body. No version, no host, no count. An answer reads memory
only and writes no audit line.

- `starting` is answered from the moment the health listener is bound, which is before the other
  listeners: the start reads its files and opens its store first, and only then binds.
- `store_unreachable` stands while the session with the database is lost. The coordinator keeps
  answering heartbeats meanwhile and orders nothing; the `store_unreachable` alert of
  `Alerts.List` says the same.
- `draining` is answered by `/readyz` from the moment the coordinator takes SIGTERM or SIGINT until
  it exits, ahead of every other token. It never turns back into `ok`.

The listener has limits: at most 1024 connections at once, ten seconds of patience for a request
head and for each write of an answer the probe does not read.

## The stop: drain delay, then connections finish

`--drain-delay-secs <N>` (default 15; 0 stops at once) is how long the coordinator goes on serving
after SIGTERM or SIGINT, counted from the signal. The sequence:

1. **At the signal.** `/readyz` answers `503 draining` at once; `/healthz` stays `200`. Every
   listener goes on accepting and serving new connections for `N` seconds, so that an edge that
   checks `/readyz` sees the `503` and stops choosing this coordinator, and what it already sent
   here is still served.
2. **After `N` seconds.** The heartbeat, private and LAN listeners accept no more connections (a
   connect is refused), and each open connection is told to finish: an HTTP/2 connection goes away
   (no new stream; the calls in flight finish), an HTTP/1.1 connection closes after the request in
   flight (an idle one closes at once), and an upgraded one (the UI's terminal, a console stream)
   ends with its socket.
3. **Held waits.** Every held `Releases.WaitConverged` call is answered at once, as at its
   `wait_secs`: the coordinator reads once more and answers `DEADLINE` unless that read ends the
   wait. CI's `set-desired` job then reports `not_converged`; a call that was cut reported
   `no_answer` before.
4. **Consoles and tunnels.** Console streams close (`coordinator_stopping`); SSH tunnels end on their
   own or are closed after `--ssh-drain-s`.
5. **Exit.** Once no connection of the three listeners is open, the process exits 0.

The health listener and the periodic work keep running until the exit, and the health listener's own
connections are not waited for. The wait for connections adds no timeout of its own: each connection
ends by the limits it already has. After the exit a probe gets no answer.

The log says it. At step 2: `stopping: the listeners accept no more connections, and the open ones
finish`, with `open=<n>`, the connections open at that moment. At step 5: `stopped: every connection
has ended`.

## The deploy check

The coordinator does not read the edge's settings and checks nothing about them while it runs; the
numbers are checked once, when the service is deployed:

- `--drain-delay-secs` is more than the edge's DNS refresh rate, plus its healthy threshold times its
  check interval, plus one more interval: the time the edge needs to see `draining` and to have
  another coordinator taking connections.
- The service's `stop_grace_period` is more than `--drain-delay-secs` plus the steps after it, which
  run one after the other: up to the console's 40 seconds of grace, then up to `--ssh-drain-s`, then
  10 seconds (one client patience) for the last connection to finish.

The edge values proposed in section 11 of the design (check interval 2 s, healthy threshold 2,
unhealthy threshold 2, DNS refresh 1 s) need about 7 seconds, so the default of 15 leaves margin.
Those checks at the edge are row M10's and not built here.

## Probing it

Any HTTP probe that treats `200` as healthy and anything else, or no answer, as not, for example:

```sh
curl --fail --silent --show-error --max-time 5 http://127.0.0.1:9100/healthz
```

Swarm's own check and the machine's monitoring use `/healthz` (a coordinator whose database does not
answer is alive: `/readyz` tells the edge to send it nothing, and it is not killed); the edge's
active check is `/readyz`.

In the compose deployment, compose runs a `healthcheck` inside the container, where there is no
client, so the compose file has none for the coordinator: the probe is the machine's own
monitoring or an orchestrator's HTTP probe. Compose's restart policy still restarts a coordinator
that exits.
